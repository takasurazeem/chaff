import SwiftUI
import chaff_ffiFFI

/// Tags, most-used first.
struct TagList: View {
    let tags: [(String, UInt32)]

    var body: some View {
        List {
            ForEach(tags, id: \.0) { name, count in
                HStack {
                    Text(name).lineLimit(1)
                    Spacer(minLength: 4)
                    Text("\(count)").foregroundStyle(.secondary).monospacedDigit()
                }
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
