import SwiftUI
import chaff_ffiFFI

/// The left column: a segmented bar switching between navigators.
///
/// # Why this shape, and not stacked panels
///
/// The web app stacks folders, tags and people in one narrow column, and it was the layout
/// decision I was least sure about — three unrelated lists competing for the same 200 points of
/// width, with the one you want usually scrolled off.
///
/// Xcode answers this and has for twenty years: **a segmented icon bar at the top of the
/// navigator**, one navigator visible at a time, and a **filter field pinned to the bottom**.
/// The bar says what is available; the filter narrows whatever is showing. Neither competes with
/// the content for space.
///
/// # The filter is at the bottom
///
/// That is Xcode's placement and it is deliberate: the field is a *control over the list above
/// it*, and putting it at the top pushes the content down by a row for something used
/// occasionally. At the bottom it is out of the way and still one click from the list.
struct Navigator: View {
    @Environment(EngineModel.self) private var model
    @Binding var chosenFolder: String?
    @State private var mode: Mode = .folders
    @State private var filter = ""

    enum Mode: String, CaseIterable, Identifiable {
        case folders, tags, people
        var id: String { rawValue }

        var symbol: String {
            switch self {
            case .folders: "folder"
            case .tags: "tag"
            case .people: "person.2"
            }
        }

        var help: String {
            switch self {
            case .folders: "Folders"
            case .tags: "Tags"
            case .people: "People"
            }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            // The bar. `Picker` with `.segmented` is the native control for exactly this, and it
            // gets keyboard navigation and VoiceOver for free.
            Picker("Navigator", selection: $mode) {
                ForEach(Mode.allCases) { m in
                    Image(systemName: m.symbol).help(m.help).tag(m)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .padding(.horizontal, 8)
            .padding(.vertical, 6)

            Divider()

            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            Divider()

            // The filter, pinned to the bottom as Xcode does.
            HStack(spacing: 4) {
                Image(systemName: "line.3.horizontal.decrease.circle")
                    .foregroundStyle(.tertiary)
                TextField("Filter", text: $filter)
                    .textFieldStyle(.plain)
                    .font(.callout)
                if !filter.isEmpty {
                    Button {
                        filter = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(.tertiary)
                    .help("Clear the filter")
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
        }
        .background(.background)
    }

    @ViewBuilder
    private var content: some View {
        switch mode {
        case .folders:
            FolderList(chosen: $chosenFolder, folders: matchingFolders)
        case .tags:
            TagList(tags: model.tags.filter { matches($0.0) })
        case .people:
            PeopleList(people: model.people.filter { matches($0.name ?? "Group \($0.id)") })
        }
    }

    /// Case-insensitive substring, which is what a filter field does everywhere else.
    private func matches(_ text: String) -> Bool {
        filter.isEmpty || text.localizedCaseInsensitiveContains(filter)
    }

    private var matchingFolders: [Folder] {
        model.folders.filter { matches(URL(fileURLWithPath: $0.path).lastPathComponent) }
    }
}
