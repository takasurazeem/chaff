import SwiftUI
import chaff_ffiFFI

/// Folder traversal, with both counts.
///
/// # Why two numbers
///
/// "How many photographs are directly in this folder" and "how many are under it" are different
/// questions and people ask both — a shoot is the first, a year is the second. The engine
/// computes both in one query; showing only one makes the other a guess.
struct FolderSidebar: View {
    @Binding var chosen: String?
    let folders: [Folder]

    var body: some View {
        List(selection: $chosen) {
            Section {
                // `nil` is not a valid `List` selection tag for `String?` without a cast, so
                // "All photographs" is a row with an explicit tag rather than the absence of
                // one.
                Label("All photographs", systemImage: "photo.stack")
                    .tag(String?.none)
                    .badge(Int(folders.map(\.recursive).max() ?? 0))
            }

            Section("Folders") {
                // **`OutlineGroup` would be the obvious control and is the wrong one here.**
                //
                // The engine returns a flat list with counts per folder. Building a tree in
                // Swift would duplicate `buildTree` from the web app — the ancestor-chain logic
                // that had a bug once — and the flat list is what the counts are keyed by.
                //
                // A disclosure tree is worth adding when folders nest deeply enough to need it;
                // most libraries are two levels.
                ForEach(folders, id: \.path) { folder in
                    FolderRow(folder: folder)
                        .tag(String?.some(folder.path))
                }
            }
        }
        .listStyle(.sidebar)
    }
}

private struct FolderRow: View {
    let folder: Folder

    var body: some View {
        HStack(spacing: 6) {
            // The last path component, because the full path is 80 characters of which the
            // useful part is the end.
            Text(URL(fileURLWithPath: folder.path).lastPathComponent)
                .lineLimit(1)
                .truncationMode(.middle)
                .help(folder.path)

            Spacer(minLength: 4)

            // Direct and recursive, distinguished by weight rather than by two columns — a
            // sidebar is narrow and two numbers side by side is a table, not a list.
            if folder.recursive != folder.direct {
                Text("\(folder.direct)")
                    .foregroundStyle(.tertiary)
                Text("\(folder.recursive)")
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            } else {
                Text("\(folder.direct)")
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
        }
        .font(.callout)
    }
}
