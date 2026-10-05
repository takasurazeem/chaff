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
    @State private var selection: Set<Int64> = []
    @State private var chosenFolder: String?
    /// The plan the user is being asked to confirm, if any.
    @State private var pendingPlan: DeletePlan?

    var body: some View {
        // `@Bindable` for the binding to the cursor: `@Environment` hands back the object, and a
        // `Binding` needs the wrapper.
        @Bindable var culling = culling

        return NavigationSplitView {
            FolderSidebar(chosen: $chosenFolder, folders: model.folders)
                .navigationSplitViewColumnWidth(min: 180, ideal: 220, max: 320)
        } detail: {
            ZStack {
                if model.photos.isEmpty && !model.isIndexing {
                    ContentUnavailableView(
                        "No library open",
                        systemImage: "photo.on.rectangle.angled",
                        description: Text("Choose a folder of photographs with ⌘O.")
                    )
                } else {
                    PhotoGrid(photos: visible, selection: $selection, cursor: $culling.cursor)
                }

                if model.isIndexing {
                    IndexingOverlay(progress: model.progress, label: model.progressLabel)
                }
            }
            .navigationTitle(
                model.library.map { URL(fileURLWithPath: $0.root).lastPathComponent } ?? "Chaff"
            )
        }
        // Delete opens **the confirmation**, and nothing else. There is no path in this
        // application that removes a file from a keystroke.
        .onDeleteCommand { beginDelete() }
        .sheet(item: $pendingPlan) { plan in
            DeleteConfirmation(
                plan: plan,
                root: model.library?.root ?? "",
                onFinished: { pendingPlan = nil }
            )
        }
        .inspector(isPresented: .constant(true)) {
            // The engine takes one photograph's detail in one call. The panel follows the
            // **selection**, falling back to nothing rather than to the cursor: an inspector
            // describing a photograph the user has not chosen is one they cannot trust.
            Inspector(photoId: selection.count == 1 ? selection.first : nil)
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
        let ids = Array(selection)
        guard !ids.isEmpty else { return }

        Task {
            do {
                pendingPlan = try await model.planDelete(photoIds: ids, root: root)
            } catch {
                model.errorMessage = model.describe(error)
            }
        }
    }

    /// The photographs in the chosen folder, or all of them.
    ///
    /// Recursive, matching the sidebar's counts: a folder that says "2,071" and then shows 250
    /// is a folder whose count is a lie.
    private var visible: [Photo] {
        guard let folder = chosenFolder else { return model.photos }
        return model.photos.filter { $0.dir == folder || $0.dir.hasPrefix(folder + "/") }
    }
}

/// A determinate bar when there is a total, indeterminate when there is not.
///
/// The scan phase genuinely has no total — the size of a tree is not known until the walk
/// finishes — so it shows a spinner and a count rather than a bar that invents a denominator.
private struct IndexingOverlay: View {
    let progress: Double?
    let label: String

    var body: some View {
        VStack(spacing: 12) {
            if let progress {
                ProgressView(value: progress)
                    .frame(width: 260)
            } else {
                ProgressView()
                    .controlSize(.small)
            }
            Text(label.isEmpty ? "Working…" : label)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: 300)
        }
        .padding(20)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 12)
    }
}
