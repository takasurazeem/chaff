import SwiftUI

/// The status line, as Xcode has under the editor.
///
/// A count that answers "what am I looking at?" — which is not the same as "how many are there",
/// and a filter that hides 2,900 photographs without saying so is a filter that looks like a
/// bug.
struct StatusBar: View {
    let shown: Int
    let total: Int
    let selection: Int

    var body: some View {
        HStack(spacing: 8) {
            if shown == total {
                Text("\(total) photographs")
            } else {
                Text("\(shown) of \(total) photographs")
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if selection > 0 {
                Text("\(selection) selected")
                    .foregroundStyle(.secondary)
            }
        }
        .font(.caption)
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
    }
}
