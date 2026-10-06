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
    @State private var chosenFolder: String?
    /// A tag or a group the grid is narrowed to — see `Narrowing` for why it is one value.
    @State private var narrowedTo: Narrowing?
    @State private var filters = Filters()
    /// The plan the user is being asked to confirm, if any.
    @State private var pendingPlan: DeletePlan?

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
                if model.photos.isEmpty && !model.isIndexing {
                    ContentUnavailableView(
                        "No library open",
                        systemImage: "photo.on.rectangle.angled",
                        description: Text("Choose a folder of photographs with ⌘O.")
                    )
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
        .sheet(isPresented: $model.showLoupe) {
            Loupe(photos: visible, isPresented: $model.showLoupe)
        }
        .inspector(isPresented: .constant(true)) {
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
        } else if let folder = chosenFolder, let root = model.library?.root {
            let absolute = root.hasSuffix("/") ? root + folder : root + "/" + folder
            narrowed = model.photos.filter {
                $0.dir == absolute || $0.dir.hasPrefix(absolute + "/")
            }
        } else {
            narrowed = model.photos
        }
        return narrowed.filter(filters.matches)
    }
}
