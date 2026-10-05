import SwiftUI
import chaff_ffiFFI

/// Folder rows, with both counts.
struct FolderList: View {
    @Binding var chosen: String?
    let folders: [Folder]

    var body: some View {
        List(selection: $chosen) {
            Label("All photographs", systemImage: "photo.stack")
                .tag(String?.none)

            ForEach(folders, id: \.path) { folder in
                HStack(spacing: 6) {
                    Text(URL(fileURLWithPath: folder.path).lastPathComponent)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .help(folder.path)
                    Spacer(minLength: 4)
                    // Direct and recursive, distinguished by weight rather than two columns —
                    // a navigator is narrow and two numbers side by side is a table, not a list.
                    if folder.recursive != folder.direct {
                        Text("\(folder.direct)").foregroundStyle(.tertiary).monospacedDigit()
                    }
                    Text("\(folder.recursive)").foregroundStyle(.secondary).monospacedDigit()
                }
                .tag(String?.some(folder.path))
            }
        }
        .listStyle(.sidebar)
    }
}
