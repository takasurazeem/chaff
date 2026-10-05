import SwiftUI
import chaff_ffiFFI

/// The grid, virtualised.
///
/// # What is actually expensive here
///
/// `LazyVGrid` only builds tiles near the viewport, so scrolling 50,000 photographs is not the
/// problem. Two things are:
///
/// 1. **The engine returns all 50,000 records in one call** (~12 MB across the FFI boundary).
///    That is one hit at open, not per frame, and it is the same shape the web app uses.
/// 2. **A thumbnail is a file read and possibly a raw decode.** The engine caches by content
///    hash and evicts on a periodic sweep rather than per write — a reviewer measured the
///    per-write sweep at **114 ms**, which is seven frames, and it would have failed this
///    view's own gate.
///
/// # What this must not do
///
/// Decode on the main actor. `NSImage(contentsOfFile:)` in `body` is the mistake that turns a
/// smooth grid into a stuttering one, and Swift has no `content-visibility` to hide it behind.
/// What VoiceOver reads for one tile.
///
/// Free and testable, rather than inline in `body` — the same reasoning as the formatting
/// helpers, and this one is easy to get subtly wrong: a rating of zero is *unrated*, not "0
/// stars", and a rejected photograph keeps the stars it had.
func accessibilityValue(isSelected: Bool) -> String {
    accessibilityValue(
        score: nil, rating: 0, rejected: false, isSelected: isSelected
    )
}

/// The full value, given what is known about the photograph.
func accessibilityValue(
    score: Int32?,
    rating: UInt8,
    rejected: Bool,
    isSelected: Bool
) -> String {
    var parts: [String] = []

    if let score {
        parts.append("score \(score)")
    } else {
        // **Said, not skipped.** A photograph with no score is one that produced no measurement
        // — a raw this build cannot decode — and silence there reads as "fine".
        parts.append("not scored")
    }

    if rejected {
        parts.append("rejected")
    }
    // Zero is *unrated*, not "zero stars". Reading "0 stars" for every unrated frame in a
    // library of 3,000 would be noise in exactly the place a user is listening for signal.
    if rating > 0 {
        parts.append("\(rating) star\(rating == 1 ? "" : "s")")
    }

    if isSelected {
        parts.append("selected")
    }
    return parts.joined(separator: ", ")
}

struct PhotoGrid: View {
    let photos: [Photo]
    @Binding var selection: Set<Int64>
    /// What the keyboard acts on. **Separate from the selection**: the selection is what a
    /// delete would move, the cursor is what `3` rates. Collapsing them means rating four
    /// photographs at once, which is never what pressing `3` means.
    @Binding var cursor: Int64?

    /// Wide enough that a tile is legible, narrow enough that a row holds several.
    private let tileWidth = Tile.width

    var body: some View {
        ScrollView {
            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: tileWidth, maximum: tileWidth * 1.6), spacing: 8)],
                spacing: 8
            ) {
                ForEach(photos, id: \.id) { photo in
                    Tile(photo: photo, isSelected: selection.contains(photo.id))
                        .onTapGesture {
                            selection = [photo.id]
                            cursor = photo.id
                        }
                }
            }
            .padding(8)
        }
        .background(.background)
    }
}

/// One photograph.
///
/// # Identity
///
/// Keyed by the engine's `id`, which is assigned once and never reused. A grid keyed by array
/// index loses scroll position and selection when the list is filtered, and the symptom looks
/// like the app forgetting what you clicked.
struct Tile: View {
    let photo: Photo
    let isSelected: Bool

    /// The same width the grid lays out at, so the tile's own frame matches the cell it is in.
    /// Read from the grid's constant rather than duplicated, because two numbers that must
    /// agree is how a grid ends up with tiles that do not fit their cells.
    static let width: CGFloat = 180

    @State private var image: NSImage?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ZStack {
                Rectangle().fill(.quaternary)
                if let image {
                    Image(nsImage: image)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                } else {
                    Text(photo.stem)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(3)
                        .padding(4)
                }
            }
            .frame(height: Self.width * 0.75)
            .clipShape(RoundedRectangle(cornerRadius: 6))
            // **Selection is an overlay, not a border on the container.**
            //
            // The web app drew it as a ring on the tile and the image covered it — an inset
            // box-shadow paints below child content. Every layer above was correct and the
            // indicator was invisible, which read as "selecting does not work". The same
            // mistake is available here and this is the shape that avoids it.
            .overlay {
                RoundedRectangle(cornerRadius: 6)
                    // `.tint` rather than `Color.accentColor`: the latter is deprecated, and
                    // `.tint` is what a user's chosen accent actually resolves to.
                    .strokeBorder(isSelected ? AnyShapeStyle(.tint) : AnyShapeStyle(.clear), lineWidth: 3)
            }

            HStack(spacing: 4) {
                Text(photo.stem)
                    .font(.caption2)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 0)
                if let score = photo.composite {
                    Text("\(Int(score))")
                        .font(.caption2)
                        .monospacedDigit()
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .task(id: photo.id) {
            // `.task(id:)` cancels and restarts when the identity changes, which is what makes
            // a recycled tile load the right photograph rather than showing the last one's.
            image = await ThumbnailLoader.shared.image(for: photo.id)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(photo.stem)
        .accessibilityValue(isSelected ? "selected" : "")
    }
}
