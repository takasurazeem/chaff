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
    @State private var selection: Set<Int64> = []
    @State private var chosenFolder: String?

    var body: some View {
        NavigationSplitView {
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
                    PhotoGrid(photos: visible, selection: $selection)
                }

                if model.isIndexing {
                    IndexingOverlay(progress: model.progress, label: model.progressLabel)
                }
            }
            .navigationTitle(
                model.library.map { URL(fileURLWithPath: $0.root).lastPathComponent } ?? "Chaff"
            )
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
