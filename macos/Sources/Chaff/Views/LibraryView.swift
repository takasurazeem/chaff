import SwiftUI
import chaff_ffiFFI

/// The whole window: folders on the left, the grid in the middle.
///
/// # Why a sidebar and not a stack
///
/// The web app stacks folders, tags and people in one narrow column, and it is the layout
/// decision I am least confident about. macOS has a control for exactly this — a sidebar beside
/// content — and a native app that reproduces a web layout has given up the reason to be native.
struct LibraryView: View {
    @Environment(EngineModel.self) private var model
    @Environment(Culling.self) private var culling
    @State private var chosenFolder: FolderSelection?
    /// A tag or a group the grid is narrowed to — see `Narrowing` for why it is one value.
    @State private var narrowedTo: Narrowing?
    @State private var filters = Filters()
    /// The plan the user is being asked to confirm, if any.
    @State private var pendingPlan: DeletePlan?
    /// The inspector column, **toggleable and remembered** — Xcode's panel is not welded shut,
    /// and neither is this one. The same defaults key is written by the toolbar button and the
    /// View menu (⌥⌘0), so all three move together; `AppStorage` persists it across launches.
    @AppStorage("chaff.showInspector") private var showInspector = true

    /// Where the last-chosen folder is remembered. **Application state, `UserDefaults`** — same
    /// reasoning as the library path in `EngineModel`, but the choice belongs to the view.
    private static let lastFolderKey = "chaff.lastFolder"

    /// Ask for a folder and open it.
    ///
    /// The same code path the menu uses, reached from the view — an `NSOpenPanel` is the only way
    /// to pick a folder, and it belongs to the view layer rather than the engine.
    /// Remember the folder chosen in the library currently open.
    ///
    /// **Only when it was a deliberate choice in the library that is open** — the path is stored
    /// absolute, so a folder name from a previous library never matches a different library.
    /// Choosing "All photographs" leaves the stored path alone; restoring nothing is the more
    /// honest memory than a stale row.
    private func remember(_ choice: FolderSelection?) {
        guard case let .folder(relative) = choice, let root = model.library?.root else { return }
        let absolute = root.hasSuffix("/") ? root + relative : root + "/" + relative
        UserDefaults.standard.set(absolute, forKey: Self.lastFolderKey)
    }

    /// Restore the folder chosen last time, if it exists in the library now open.
    ///
    /// A stale path — the folder moved, a different library — is silently dropped rather than
    /// applied: a stored path that narrowed the grid to nothing would be a state the user never
    /// saw being chosen.
    private func restoreLastFolder() {
        guard
            let absolute = UserDefaults.standard.string(forKey: Self.lastFolderKey),
            let root = model.library?.root,
            absolute.hasPrefix(root.hasSuffix("/") ? root : root + "/")
        else { return }

        let relative = String(absolute.dropFirst(root.hasSuffix("/") ? root.count : root.count + 1))
        guard !relative.isEmpty,
            model.folders.contains(where: { $0.path == relative })
        else { return }
        chosenFolder = .folder(relative)
    }

    private func chooseFolder() async {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Open"
        panel.message = "Choose a folder of photographs. Chaff reads it in place."
        guard panel.runModal() == .OK, let url = panel.url else { return }
        await model.open(url)
    }

    /// What to show when nothing is open.
    ///
    /// **The first thing a new user sees**, and it was an empty grid with no explanation. An empty
    /// grid reads as "this library has no photographs" — which is a different statement from "no
    /// library is open", and the two have different answers.
    @ViewBuilder
    private var emptyState: some View {
        ContentUnavailableView {
            Label("No library open", systemImage: "photo.on.rectangle.angled")
        } description: {
            Text(
                "Chaff reads a folder of photographs in place. It does not move or copy anything until you ask it to."
            )
        } actions: {
            Button("Open a Folder…") { Task { await chooseFolder() } }
                .buttonStyle(.borderedProminent)
            Text("⌘O").font(.caption).foregroundStyle(.tertiary)
        }
    }

    var body: some View {
        // `@Bindable` for the bindings: `@Environment` hands back the object, and a `Binding`
        // needs the wrapper.
        @Bindable var culling = culling
        @Bindable var model = model

        return NavigationSplitView {
            Navigator(chosenFolder: $chosenFolder, narrowedTo: $narrowedTo)
                .navigationSplitViewColumnWidth(min: 200, ideal: 240, max: 340)
        } detail: {
            ZStack {
                // **Two states, not one.** "No library is open" and "this library has no
                // photographs" are different statements with different answers — the first needs
                // ⌘O, the second needs an explanation of why a folder full of files produced
                // nothing. The condition below was one branch for both, so a user who opened an
                // empty folder was told to open a folder.
                if model.library == nil {
                    emptyState
                } else if model.photos.isEmpty && !model.isIndexing {
                    ContentUnavailableView {
                        Label("No photographs here", systemImage: "photo")
                    } description: {
                        Text(
                            "The folder was read and nothing in it looked like a photograph. Raw files this build cannot decode, or a folder that only holds video."
                        )
                    } actions: {
                        Button("Re-index") { Task { await model.reindex() } }
                    }
                } else {
                    VStack(spacing: 0) {
                        // **Above the grid, not in a menu.** A filter you have to open a menu to
                        // see is one you forget is applied — and then the grid looks broken.
                        FilterBar(filters: $filters, photos: model.photos)
                        Divider()
                        PhotoGrid(photos: visible, selection: $model.selection, cursor: $culling.cursor)
                    }
                }

                if model.isIndexing {
                    IndexingOverlay(
                        progress: model.progress,
                        label: model.progressLabel,
                        eta: model.eta,
                        onCancel: { model.cancelIndexing() }
                    )
                }
            }
            // Xcode's status line, along the bottom of the editor: what is shown, out of what.
            .safeAreaInset(edge: .bottom, spacing: 0) {
                StatusBar(
                    shown: visible.count,
                    total: model.photos.count,
                    selection: model.selection.count,
                    narrowing: narrowedTo?.label
                )
            }
            .navigationTitle(
                model.library.map { URL(fileURLWithPath: $0.root).lastPathComponent } ?? "Chaff"
            )
            .toolbar {
                // **Xcode's right edge: the inspector's control, in the toolbar.** The switcher
                // lives on the leading side, the inspector's on the trailing side, and the
                // content between them is the editor — the same three columns Xcode puts its
                // navigators, editors and inspectors into.
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        showInspector.toggle()
                    } label: {
                        Image(systemName: "sidebar.squares.right")
                    }
                    .help(showInspector ? "Hide Inspector (⌥⌘0)" : "Show Inspector (⌥⌘0)")
                    .keyboardShortcut("0", modifiers: [.option, .command])
                    .accessibilityLabel(showInspector ? "Hide inspector" : "Show inspector")
                }
            }
        }
        // Delete opens **the confirmation**, and nothing else. There is no path in this
        // application that removes a file from a keystroke.
        .onDeleteCommand { beginDelete() }
        // **Every failure the model records, shown.**
        //
        // `errorMessage` was set in eight places and displayed in none — so a failed delete, a
        // failed name, a failed pass all did nothing visible. The operation simply did not
        // happen and the interface looked as though it had.
        //
        // An alert rather than a banner: these are failures of something the user asked for, and
        // they need acknowledging rather than scrolling past.
        .alert(
            "Something went wrong",
            isPresented: Binding(
                get: { model.errorMessage != nil },
                set: { if !$0 { model.errorMessage = nil } }
            ),
            presenting: model.errorMessage
        ) { _ in
            Button("OK", role: .cancel) { model.errorMessage = nil }
        } message: { message in
            Text(message)
        }
        .sheet(item: $pendingPlan) { plan in
            DeleteConfirmation(
                plan: plan,
                root: model.library?.root ?? "",
                onFinished: { pendingPlan = nil }
            )
        }
        // The loupe pages through **what the grid is showing**, not the whole library — paging
        // past the end of a filtered view into photographs the filter excluded is how a user
        // rates something they were not looking at.
        // The grid's visible list, published for Select All. See `visibleIds`.
        .onChange(of: visible.map(\.id)) { _, ids in model.visibleIds = ids }
        .onAppear { model.visibleIds = visible.map(\.id) }
        // **Reopen what was open last.** The model remembers the library path; the *choice
        // inside* the library is this view's state, and this is where it is restored. Both run
        // once the reopen finishes, so the folder row the saved path points to exists when the
        // list is given it.
        .task {
            await model.reopenLastLibrary()
            restoreLastFolder()
        }
        // **A new library starts narrowed to nothing.** A folder chosen in the last library
        // matches nothing in this one — an empty grid with no explanation.
        .onChange(of: model.library) { _, library in
            chosenFolder = nil
            narrowedTo = nil
            if library != nil { restoreLastFolder() }
        }
        // **One narrowing at a time.** `visible` lets the tag or group win over the folder, so
        // choosing a folder while a tag was active changed nothing the user could see. The last
        // thing clicked is what the grid shows.
        .onChange(of: chosenFolder) { _, choice in
            guard choice != nil else { return }
            narrowedTo = nil
            remember(choice)
        }
        .onChange(of: narrowedTo) { _, value in
            if value != nil { chosenFolder = nil }
        }
        .sheet(isPresented: $model.showLoupe) {
            Loupe(photos: visible, isPresented: $model.showLoupe)
        }
        .inspector(isPresented: $showInspector) {
            // The engine takes one photograph's detail in one call. The panel follows the
            // **selection**, falling back to nothing rather than to the cursor: an inspector
            // describing a photograph the user has not chosen is one they cannot trust.
            Inspector(photoId: model.selection.count == 1 ? model.selection.first : nil)
                .inspectorColumnWidth(min: 240, ideal: 300, max: 420)
        }
    }

    /// Ask the engine what the selection would move, and show it.
    ///
    /// The engine **hashes every file here**, while the user is looking at the list, because
    /// that is what the confirmation verifies against. It is also where the refusals come
    /// from — a selection that cannot be moved is refused before the sheet offers a button.
    private func beginDelete() {
        guard let root = model.library?.root else { return }
        let ids = Array(model.selection)
        guard !ids.isEmpty else { return }

        Task {
            do {
                pendingPlan = try await model.planDelete(photoIds: ids, root: root)
            } catch {
                model.errorMessage = model.describe(error)
            }
        }
    }

    /// The photographs the grid is showing, after the folder and the narrowing.
    ///
    /// **Narrowing wins over the folder.** They answer the same question, and the last thing the
    /// user clicked is the answer they meant — so choosing a tag clears the folder rather than
    /// intersecting with it, and vice versa. Intersecting would show an empty grid with no
    /// explanation of why.
    /// The photographs in the chosen folder, or all of them.
    ///
    /// Recursive, matching the sidebar's counts: a folder that says "2,071" and then shows 250
    /// is a folder whose count is a lie.
    private var visible: [Photo] {
        // **The folder arrives relative and the photograph's path is absolute.**
        //
        // `folders` strips the library root so the navigator does not show the user's whole
        // filesystem — which means comparing `chosenFolder` against `photo.dir` directly matches
        // nothing at all. The root goes back on here, at the one place that needs it.
        //
        // The failure mode is the bad kind: an empty grid, no error, and a folder that says 2,071
        // photographs.
        // The folder, the tag or the group first, then the chips — **composed**, because they
        // answer different questions and a user who selected a folder and then a band means
        // both.
        let narrowed: [Photo]
        if let narrowedTo {
            narrowed = model.photos.filter {
                narrowedTo.matches(
                    photoId: $0.id,
                    tagsByPhoto: model.tagsByPhoto,
                    peopleByPhoto: model.peopleByPhoto
                )
            }
        } else if case let .folder(relative) = chosenFolder, let root = model.library?.root {
            let absolute = root.hasSuffix("/") ? root + relative : root + "/" + relative
            narrowed = model.photos.filter {
                $0.dir == absolute || $0.dir.hasPrefix(absolute + "/")
            }
        } else {
            narrowed = model.photos
        }
        // **Search and sort last**, because they act on what survived the filters — sorting a
        // list and then filtering it would reorder the result of the filter, which is the same
        // thing but does the expensive work twice.
        return filters.sort(narrowed.filter(filters.matches).filter(filters.matchesSearch))
    }
}
