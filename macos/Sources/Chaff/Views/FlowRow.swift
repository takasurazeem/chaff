import SwiftUI

/// Items laid out in a row, wrapping onto the next line when they run out of width.
///
/// # Why not `HStack`
///
/// A fixed `HStack` of tags overflows the inspector and truncates, so the fourth tag is invisible
/// in a 240-point panel — which is the width this panel actually is. `LazyVGrid` would work and
/// forces every cell to the widest one's width, which for tags of different lengths leaves gaps
/// where a tag should be.
///
/// # Why not a `List`
///
/// Four one-word rows in a vertical list cost four rows of height for four words.
struct FlowRow<Item: Hashable, Content: View>: View {
    private let items: [Item]
    private let content: (Item) -> Content
    private let spacing: CGFloat

    init(_ items: [Item], spacing: CGFloat = 4, @ViewBuilder content: @escaping (Item) -> Content) {
        self.items = items
        self.spacing = spacing
        self.content = content
    }

    var body: some View {
        // `Layout` rather than a geometry reader: it is the API for exactly this, it reports its
        // own size correctly to the parent, and it does not need a second pass.
        FlowLayout(spacing: spacing) {
            ForEach(items, id: \.self) { content($0) }
        }
    }
}

private struct FlowLayout: Layout {
    let spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0

        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            // **Wrap before overflowing, not after.** A row that places the item and then finds
            // it does not fit pushes the tag past the panel's edge where it is clipped and
            // unreadable.
            if x > 0, x + size.width > maxWidth {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: maxWidth == .infinity ? x : maxWidth, height: y + rowHeight)
    }

    func placeSubviews(
        in bounds: CGRect,
        proposal: ProposedViewSize,
        subviews: Subviews,
        cache: inout ()
    ) {
        var x = bounds.minX
        var y = bounds.minY
        var rowHeight: CGFloat = 0

        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}
