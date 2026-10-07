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
    /// The gutters, in points. One constant: the grid's spacing, its padding, and the column
    /// maths below must agree, and three copies of `8` is how they drift apart.
    private let spacing: CGFloat = 8

    /// How many tiles fit on a row at a given width.
    ///
    /// **The single answer to this question.** The grid items and the arrow-key navigation both
    /// derive from it: the grid used to lay out with `GridItem(.adaptive(...))` while navigation
    /// used this function, and two answers meant up/down could land on a different row than the
    /// one on screen.
    private func columnCount(in width: CGFloat) -> Int {
        let usable = width - spacing * 2 // the padding on each side
        return max(1, Int((usable + spacing) / (tileWidth + spacing)))
    }

    /// Move the cursor, and the selection with it.
    private func move(_ direction: GridNavigation.Move, extending: Bool) {
        // A cursor that has never been set starts at the top rather than doing nothing — pressing
        // an arrow in a fresh library should show you the first photograph.
        guard let current = cursor.flatMap({ id in photos.firstIndex { $0.id == id } }) else {
            guard let first = GridNavigation.destination(from: 0, move: .first, count: photos.count, columns: columns)
            else { return }
            apply(index: first, extending: false)
            return
        }
        guard let next = GridNavigation.destination(
            from: current, move: direction, count: photos.count, columns: columns
        ) else { return }
        apply(index: next, extending: extending)
    }

    private func apply(index: Int, extending: Bool) {
        let result = GridNavigation.selection(
            after: index, photos: photos, anchor: anchor, extending: extending
        )
        selection = result.ids
        anchor = result.anchor
        cursor = photos[index].id
    }

    /// How many tiles fit on a row, measured from the geometry.
    ///
    /// Up and down cannot be derived from the photograph list — they depend on the window width
    /// and the tile size — so the grid measures it and the navigation arithmetic uses it.
    @State private var columns = 1
    /// Where a shift-extended range started, so extending and contracting works.
    @State private var anchor: Int64?

    var body: some View {
        GeometryReader { geo in
            // Resolved here, in the layout, rather than read from state: state lags a frame
            // behind a resize and the first render has no measurement yet. `columns` is still
            // published below for the arrow keys, which cannot read this scope.
            let cols = columnCount(in: geo.size.width)
            ScrollView {
                LazyVGrid(
                    columns: Array(
                        repeating: GridItem(.flexible(minimum: 0), spacing: spacing),
                        count: cols
                    ),
                    spacing: spacing
                ) {
                    ForEach(photos, id: \.id) { photo in
                        Tile(photo: photo, isSelected: selection.contains(photo.id))
                            .onTapGesture {
                                // **A click sets the anchor**, so a following shift-arrow extends
                                // from where the user actually clicked rather than from wherever
                                // the cursor last was.
                                selection = [photo.id]
                                cursor = photo.id
                                anchor = photo.id
                            }
                    }
                }
                .padding(spacing)
            }
            // **The culling loop, which did not exist.**
            //
            // `1`–`5` in the menu rate the *cursor*, and the cursor could only be set by
            // clicking — so you could rate a photograph and then had to reach for the mouse to
            // reach the next one. For a tool whose whole purpose is going through thousands of
            // frames, that is the difference between usable and not.
            //
            // The arrows arrive through `onMoveCommand`, which is the platform's own path for
            // them: a grid inside a scroll view does not reliably see arrow keys as key presses,
            // because the scroll view consumes them first.
            .onMoveCommand { direction in
                switch direction {
                case .left: move(.left, extending: false)
                case .right: move(.right, extending: false)
                case .up: move(.up, extending: false)
                case .down: move(.down, extending: false)
                @unknown default: break
                }
            }
            .onKeyPress(.home) { move(.first, extending: false); return .handled }
            .onKeyPress(.end) { move(.last, extending: false); return .handled }
            .onKeyPress(.pageUp) { move(.pageUp, extending: false); return .handled }
            .onKeyPress(.pageDown) { move(.pageDown, extending: false); return .handled }
            .onKeyPress(.escape) {
                // **Clearing the selection, which is how a user says "never mind"** after
                // selecting twelve frames for a delete and thinking better of it.
                selection = []
                anchor = nil
                return .handled
            }
            .onAppear { columns = cols }
            .onChange(of: geo.size.width) { _, width in columns = columnCount(in: width) }
        }
        // The same mistake as the navigator's: `.background` is opaque, so this flattened the
        // editor pane against a colour instead of letting it sit on the window's material.
        //
        // A grid of photographs is content and gets **no glass** — that rule stands. What it does
        // not need is an opaque plate painted underneath it either; the window already has a
        // background and the tiles draw on top of it.
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
    /// A 4:3 cell. Every cell is the same **height as well as width**, which is what makes a
    /// grid read as a grid: a row of different-height tiles reads as scattered scrapbook.
    static let imageHeight: CGFloat = width * 0.75

    @State private var image: NSImage?
    @State private var hovering = false

    var body: some View {
        ImageArena { size in
            ZStack(alignment: .bottom) {
                // The neutral plate behind a photograph that has not arrived, and the letterbox
                // bands beside one whose shape does not match the cell. One fill for both.
                Rectangle().fill(.quaternary)
                if let image {
                    // **Fit, not fill.** A culling tool has to answer "would this frame crop
                    // well", and a cover-cropped thumbnail hides the frame's own shape — which
                    // is exactly the neighbour-overlap class of bug `scaledToFill` on a
                    // flexible cell produced. `.resizable().scaledToFit()` can only ever
                    // *shrink* to the frame below it, so an image with no place left to grow
                    // cannot paint over its neighbour no matter what the proposal is.
                    Image(nsImage: image)
                        .resizable()
                        .scaledToFit()
                        .frame(width: size.width, height: size.height)
                        .opacity(photo.rejected ? 0.45 : 1)
                    metadata
                }
            }
            .frame(width: size.width, height: size.height)
            .clipShape(RoundedRectangle(cornerRadius: 6))
            .overlay(alignment: .topTrailing) {
                if photo.rejected {
                    Image(systemName: "xmark.octagon.fill")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(.red)
                        .padding(5)
                        .background(Circle().fill(.white.opacity(0.85)).padding(2))
                        .padding(4)
                }
            }
            // A hairline so a bright photograph reads against a light window. Firms up under the
            // pointer — definition in the place the user is looking — and dies when nothing is
            // asking about the tile.
            .overlay {
                RoundedRectangle(cornerRadius: 6)
                    .strokeBorder(Color.primary.opacity(isSelected || hovering ? 0.25 : 0.12), lineWidth: 1)
            }
            // **Selection is an overlay, not a border on the container.**
            //
            // The web app drew it as a ring on the tile and the image covered it — an inset
            // box-shadow paints below child content. Every layer above was correct and the
            // indicator was invisible, which read as "selecting does not work". An overlay drawn
            // *after* the image cannot be covered by it.
            .overlay {
                RoundedRectangle(cornerRadius: 6)
                    // `.tint` rather than `Color.accentColor`: the latter is deprecated, and
                    // `.tint` is what a user's chosen accent actually resolves to.
                    .strokeBorder(isSelected ? AnyShapeStyle(.tint) : AnyShapeStyle(.clear), lineWidth: 3)
            }
            .animation(.easeOut(duration: 0.12), value: isSelected)
            .animation(.easeOut(duration: 0.12), value: hovering)
        }
        .frame(maxWidth: .infinity)
        .frame(height: Self.imageHeight)
        .onHover { hovering = $0 }
        .task(id: photo.id) {
            // `.task(id:)` cancels and restarts when the identity changes, which is what makes
            // a recycled tile load the right photograph rather than showing the last one's.
            image = await ThumbnailLoader.shared.image(for: photo.id)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(photo.stem)
        .accessibilityValue(isSelected ? "selected" : "")
    }

    /// What runs along a photograph's bottom edge: the filename, the rating and the score.
    ///
    /// **Painted on the photograph, not below it** — this is the cell every culling app shares
    /// (Lightroom, Capture One, Photo Mechanic). Metadata under the tile costs a row of vertical
    /// space per tile and makes the columns' text collide with the next row; a strip that lives
    /// inside the clipped cell cannot.
    private var metadata: some View {
        HStack(spacing: 4) {
            Text(photo.stem)
                .font(.system(size: 9, weight: .medium, design: .monospaced))
                .lineLimit(1)
                .truncationMode(.middle)
                .foregroundStyle(.white)
            Spacer(minLength: 0)
            Stars(photo.rating)
            if let score = photo.composite {
                Text("\(Int(score))")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .monospacedDigit()
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(.black.opacity(0.45)))
                    .foregroundStyle(scoreColor(score))
            }
        }
        .padding(.leading, 7)
        .padding(.trailing, 4)
        .padding(.bottom, 4)
        .background(
            // A scrim, not a plate: the photograph keeps the last of its height, and one
            // gradient serves every cell's edge the way the loupe's contents do.
            LinearGradient(
                colors: [.black.opacity(0), .black.opacity(0.35)],
                startPoint: .top,
                endPoint: .bottom
            )
            .allowsHitTesting(false)
        )
    }

    /// A colour a score earns, quietly: to thirds. Keep is green, review white, reject orange —
    /// the same vocabulary the FilterBar's chips, the only strong tint this grid allows itself.
    private func scoreColor(_ score: Double) -> Color {
        switch Int(score) {
        case 67...: Color(red: 0.45, green: 0.8, blue: 0.55)
        case 34...66: .white
        default: Color(red: 0.94, green: 0.62, blue: 0.38)
        }
    }

    /// The star row, `nil` when the photograph is unrated — zero is *unrated*, not "no stars",
    /// which is the grid's own accessibility rule kept in its pixels.
    private struct Stars: View {
        let rating: UInt8

        init?(_ rating: UInt8) {
            guard rating > 0 else { return nil }
            self.rating = rating
        }

        var body: some View {
            HStack(spacing: 1) {
                ForEach(0 ..< Int(rating), id: \.self) { _ in
                    Image(systemName: "star.fill")
                        .font(.system(size: 8))
                        .foregroundStyle(.yellow)
                        .shadow(color: .black.opacity(0.4), radius: 1, y: 0.5)
                }
            }
        }
    }
}

/// A container that measures its own cell and hands exact points to `content`.
///
/// **Why this exists, when `.frame(maxWidth: .infinity)` seemed to.** The overlap in the grid was
/// a photograph painting past its column. `maxWidth` asks what the *parent* proposes, and
/// SwiftUI proposes a lot for an image that has an intrinsic size — resized fills, the
/// aspect-ratio content then sizes the frame rather than a fixed frame sizing it. `GeometryReader`
/// does not negotiate: it takes exactly the space the cell gives it, and the `size` it hands out
/// is measured, not assumed. Every photograph below is framed in those *measured* points, so an
/// image cannot be wider than its cell by construction — there is no layout path into overflow.
private struct ImageArena<Content: View>: View {
    @ViewBuilder var content: (CGSize) -> Content

    var body: some View {
        GeometryReader { geo in
            content(geo.size)
        }
    }
}
