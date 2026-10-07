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
    @Binding var chosenFolder: FolderSelection?
    /// What the grid is narrowed to, when it is narrowed by a tag or a group rather than a
    /// folder. **One selection, not two** — a folder and a tag at once is a filter combination
    /// nothing in the interface explains, and the status line can only describe one.
    @Binding var narrowedTo: Narrowing?
    /// The navigator shown, **remembered across launches** like Xcode remembers which navigator
    /// was last showing. On `AppStorage`, not `@State`, so the Cull menu's ⌘1–⌘3 items — which
    /// write the same key — move this picker in both directions: the menu cannot reach another
    /// view's `@State`, but application defaults are reachable from both.
    @AppStorage("chaff.navigatorMode") private var mode: Mode = .folders
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
            // **The switcher, pinned to the top of the sidebar itself.**
            //
            // It had lived in the window toolbar — and Xcode's does *not*: the reference
            // window shows the navigator's icon row as a strip inside the sidebar's own top,
            // under the traffic lights, with the window toolbar belonging to the wider
            // window's controls. A control at the top of the column it governs also reads
            // better: the segmented bar-makers here, and the filter below narrows them.
            HStack(spacing: 4) {
                Picker("Navigator", selection: $mode) {
                    ForEach(Mode.allCases) { m in
                        Image(systemName: m.symbol)
                            .help("\(m.help) (⌘\(Mode.allCases.firstIndex(of: m)! + 1))")
                            .accessibilityLabel(m.help)
                            .tag(m)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)

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
    }

    @ViewBuilder
    private var content: some View {
        switch mode {
        case .folders:
            FolderList(chosen: $chosenFolder, folders: matchingFolders)
        case .tags:
            TagList(tags: model.tags.filter { matches($0.0) }, narrowedTo: $narrowedTo)
        case .people:
            PeopleList(people: model.people.filter { matches($0.name ?? "Group \($0.id)") }, narrowedTo: $narrowedTo)
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
