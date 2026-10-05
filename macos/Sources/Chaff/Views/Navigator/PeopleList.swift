import SwiftUI
import chaff_ffiFFI

/// Suggested people, most photographs first.
struct PeopleList: View {
    /// A `Binding<Set>` because `List(selection:)` wants one, but only ever holds one value.
    private var selection: Binding<Set<Narrowing>> {
        Binding(
            get: { narrowedTo.map { [$0] } ?? [] },
            set: { narrowedTo = $0.first }
        )
    }

    let people: [Person]
    @Binding var narrowedTo: Narrowing?
    /// The group a sheet is open for, if any. Two separate optionals because the two sheets do
    /// different things and a single value would need a mode beside it.
    @State private var naming: Person?
    @State private var merging: Person?

    var body: some View {
        List(selection: selection) {
            ForEach(people, id: \.id) { person in
                HStack {
                    Text(person.name ?? "Group \(person.id)")
                        .lineLimit(1)
                        // **A group with no name is a suggestion**, and the styling says so.
                        // Italic and secondary until someone types a name, which is what
                        // confirms it — treating a cluster as fact is how a stranger's face
                        // ends up under someone's name.
                        .italic(person.name == nil)
                        .foregroundStyle(person.name == nil ? .secondary : .primary)
                    Spacer(minLength: 4)
                    Text("\(person.photos)").foregroundStyle(.secondary).monospacedDigit()
                }
                .tag(Narrowing.person(id: person.id, name: person.name))
                // **A context menu, because these are corrections.** Naming and merging are
                // things you do *about* a group rather than with it, and a row full of buttons
                // would make the common action — selecting it to see its photographs — the
                // hardest one to hit.
                .contextMenu {
                    Button(person.name == nil ? "Name…" : "Rename…") { naming = person }
                    Button("Merge into…") { merging = person }
                        .disabled(people.count < 2)
                }
            }
        }
        .listStyle(.sidebar)
        .sheet(item: $naming) { person in
            NamePersonSheet(person: person)
        }
        .sheet(item: $merging) { person in
            MergePeopleSheet(from: person)
        }
        .overlay {
            if people.isEmpty {
                ContentUnavailableView {
                    Label("No groups yet", systemImage: "person.2")
                } description: {
                    // **A group is a suggestion, and the wording says so.** Treating a cluster
                    // as fact is how a stranger's face ends up under someone's name.
                    Text("Faces are found and grouped on this machine. A group is a suggestion, not a name.")
                } actions: {
                    Text("⇧⌘F").font(.caption).foregroundStyle(.tertiary)
                }
            }
        }
    }
}
