import SwiftUI
import chaff_ffiFFI

/// Suggested people, most photographs first.
struct PeopleList: View {
    let people: [Person]

    var body: some View {
        List {
            ForEach(people, id: \.id) { person in
                HStack {
                    Text(person.name ?? "Group \(person.id)").lineLimit(1)
                    Spacer(minLength: 4)
                    Text("\(person.photos)").foregroundStyle(.secondary).monospacedDigit()
                }
            }
        }
        .listStyle(.sidebar)
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
