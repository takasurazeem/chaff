import SwiftUI
import chaff_ffiFFI

/// Two to four frames, side by side.
///
/// # Why this is not the loupe with more panes
///
/// The loupe shows one photograph. Judging a burst means seeing them **at the same size, at the
/// same time** — a keeper and a near-miss differ by a little focus, a little expression, half a
/// stop of exposure, and arrowing between them one at a time makes you remember the last one
/// instead of seeing them together.
///
/// # Why one zoom, not one per pane
///
/// The web app's reasoning, kept: four panes each with their own zoom would be four photographs
/// to operate. The point is comparison, so the zoom is **one** — and the same part of each frame
/// stays under the cursor.
///
/// # What it does not do
///
/// It does not pan. A shared zoom with no pan shows the centre of each frame at 100%, which is
/// where a portrait's focus usually is — and a pan that has to stay synchronised across four
/// panes is a second interaction this can wait for.
struct Compare: View {
    @Environment(EngineModel.self) private var model
    @Environment(Culling.self) private var culling

    /// The photographs being compared — the grid's visible list, so it compares **what the user
    /// was looking at** rather than the whole library.
    let photos: [Photo]
    @Binding var isPresented: Bool

    @State private var zoomed = false

    /// The frames to show: the selection if there is one, otherwise the cursor and its neighbours.
    ///
    /// **Capped at four.** More than that and each pane is too narrow to judge, which defeats the
    /// point — the web app caps it for the same reason.
    private var frames: [Photo] {
        photos.filter { culling.comparing.contains($0.id) }.prefix(4).map { $0 }
    }

    var body: some View {
        ZStack {
            // The same black as the loupe: a photograph being judged gets neither glass nor a
            // material behind it, because any tint is a colour cast on the thing being judged.
            Color.black.ignoresSafeArea()

            if frames.isEmpty {
                ContentUnavailableView(
                    "Nothing to compare",
                    systemImage: "rectangle.split.2x1",
                    description: Text(
                        "Select two to four photographs, then press C."
                    )
                )
            } else {
                VStack(spacing: 0) {
                    HStack(spacing: 1) {
                        ForEach(frames, id: \.id) { photo in
                            Pane(photo: photo, zoomed: zoomed)
                        }
                    }

                    HStack(spacing: 14) {
                        ForEach(frames, id: \.id) { photo in
                            VStack(spacing: 2) {
                                Text(photo.stem).font(.caption).lineLimit(1)
                                HStack(spacing: 6) {
                                    if let c = photo.composite {
                                        Text("\(Int(c))").monospacedDigit()
                                    }
                                    if let f = photo.focus {
                                        // **The focus percentile, which is what a burst is usually
                                        // judged on** — and the number that makes "this one is
                                        // sharper" checkable rather than a feeling.
                                        Text("focus \(Int(f))")
                                            .foregroundStyle(f >= 75 ? .green : .secondary)
                                    }
                                    if photo.rejected {
                                        Text("rejected").foregroundStyle(.red)
                                    }
                                }
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                            }
                            .frame(maxWidth: .infinity)
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                }
            }

            // **The shortcut, said on screen.** Compare is opened from a menu a user may not have
            // read, and the two keys that operate it are not guessable.
            VStack {
                HStack {
                    Spacer()
                    Text(zoomed ? "Z — fit   ·   Esc — close" : "Z — 100%   ·   Esc — close")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .padding(12)
                }
                Spacer()
            }
        }
        .onKeyPress(.escape) { isPresented = false; return .handled }
        .onKeyPress(keys: ["z", "Z"]) { _ in
            zoomed.toggle()
            return .handled
        }
    }
}

/// One frame in the comparison.
private struct Pane: View {
    let photo: Photo
    let zoomed: Bool

    @State private var image: NSImage?

    var body: some View {
        Group {
            if let image {
                if zoomed {
                    // **At 100%, clipped rather than scaled.** The panes are narrow, so a fitted
                    // 2048-pixel render would be *smaller* than the fitted loupe — the opposite
                    // of zooming. Clipping shows the centre at full resolution, which is what
                    // comparing focus needs.
                    Image(nsImage: image)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .clipped()
                } else {
                    Image(nsImage: image)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                ZStack {
                    Rectangle().fill(.black)
                    ProgressView().controlSize(.small).tint(.white)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task(id: Load(cursor: photo.id, zoomed: zoomed)) {
            image = zoomed
                ? await ThumbnailLoader.shared.zoom(for: photo.id)
                : await ThumbnailLoader.shared.loupe(for: photo.id)
        }
    }

    /// Both the photograph and which render of it, as one identity.
    ///
    /// Two separate `.task(id:)` modifiers would race: toggling zoom would start a second load
    /// while the first was in flight, and whichever finished last would win — which is a pane
    /// showing the wrong resolution with nothing to explain it.
    private struct Load: Equatable {
        let cursor: Int64
        let zoomed: Bool
    }
}
