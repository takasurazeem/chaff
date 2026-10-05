import SwiftUI

/// A determinate bar when there is a total, indeterminate when there is not.
///
/// The scan phase genuinely has no total — the size of a tree is not known until the walk
/// finishes — so it shows a spinner and a count rather than a bar that invents a denominator.
struct IndexingOverlay: View {
    let progress: Double?
    let label: String
    /// What is left, in words — or `nil` when it cannot be justified.
    var eta: String?
    var onCancel: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            if let progress {
                ProgressView(value: progress)
                    .frame(width: 260)
            } else {
                ProgressView()
                    .controlSize(.small)
            }
            Text(label.isEmpty ? "Working…" : label)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: 300)

            // Cancel and an estimate — issue #73. The estimate is computed from the rate the
            // pass has actually achieved, and **says nothing it cannot justify**: under ten
            // seconds it says so rather than printing a countdown that jitters.
            HStack(spacing: 10) {
                if let eta {
                    Text(eta).font(.caption).foregroundStyle(.tertiary)
                }
                Button("Cancel") { onCancel() }
                    .chaffFloatingButton()
            }
        }
        .padding(20)
        // **A floating surface, because this genuinely floats.**
        //
        // It is a card over the grid, not part of the layout — which is the test for whether
        // glass is right. The rule kept throughout: glass on the chrome, never on the content.
        // A photograph behind glass is a colour cast on the photograph, and this tool's job is
        // to show the frame accurately.
        //
        // Glass on macOS 26, a material before it — see `Compatibility.swift`.
        .chaffFloatingSurface()
    }
}
