import SwiftUI
import chaff_ffiFFI

/// What the folder list is narrowed to.
///
/// # A named enumeration, not a raw `String?`
///
/// `List(selection:)` infers its value type **from the binding alone**, and a row only counts
/// as selectable when its `.tag` is that same type. With `chosen: String?` the inferred value
/// type is `String` — so the `.tag(String?.some(path))` rows were `Optional<String>`, a type the
/// list could never match, and a click moved the AppKit highlight without ever writing the
/// binding: the sidebar lit up, the grid did not change.
///
/// A `nil` selection meant "all photographs", and optionals cannot be tags — so the one honest
/// shape is this: a real type, `all` named, the folder's path carried as an associated value.
/// The tag, the binding and the state all become the same type, and tapping updates.
///
/// Xcode's project navigator does the same thing underneath: one selectable value per row, the
/// row that means "nothing narrowed" included.
enum FolderSelection: Hashable {
    case all
    case folder(String)

    /// The path the grid filters to, or `nil` when the whole library is showing.
    var path: String? {
        if case let .folder(path) = self { return path }
        return nil
    }
}

/// Folder rows, with both counts.
struct FolderList: View {
    @Binding var chosen: FolderSelection?
    let folders: [Folder]

    var body: some View {
        List(selection: $chosen) {
            Label("All photographs", systemImage: "photo.stack")
                .tag(FolderSelection.all)

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
                .tag(FolderSelection.folder(folder.path))
            }
        }
        .listStyle(.sidebar)
    }
}
