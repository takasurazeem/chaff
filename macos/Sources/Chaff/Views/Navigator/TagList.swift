import SwiftUI
import chaff_ffiFFI

/// Tags, most-used first.
struct TagList: View {
    /// A `Binding<Set>` because `List(selection:)` wants one, but only ever holds one value.
    private var selection: Binding<Set<Narrowing>> {
        Binding(
            get: { narrowedTo.map { [$0] } ?? [] },
            set: { narrowedTo = $0.first }
        )
    }

    let tags: [(String, UInt32)]
    @Binding var narrowedTo: Narrowing?

    var body: some View {
        List(selection: selection) {
            ForEach(tags, id: \.0) { name, count in
                HStack {
                    Text(name).lineLimit(1)
                    Spacer(minLength: 4)
                    Text("\(count)").foregroundStyle(.secondary).monospacedDigit()
                }
                .tag(Narrowing.tag(name))
            }
        }
        .listStyle(.sidebar)
        .overlay {
            if tags.isEmpty {
                // **An empty state that says how to fill it.** "No tags yet" alone is a dead
                // end; the shortcut is what makes it actionable, and it is in the menu where a
                // user can also find it.
                ContentUnavailableView {
                    Label("No tags yet", systemImage: "tag")
                } description: {
                    Text("Tagging sends a downscaled copy to your model server — or runs CLIP on this machine if you have none. The original and its GPS never leave.")
                } actions: {
                    Text("⇧⌘T").font(.caption).foregroundStyle(.tertiary)
                }
            }
        }
    }
}
