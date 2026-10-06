import Foundation
import chaff_ffiFFI

/// Moving the cursor and the selection around a grid.
///
/// # Why this is a type and not a switch in a view
///
/// **The grid had no keyboard navigation at all.** The menu rates with `1`–`5` and they act on the
/// cursor — but the cursor could only be set by *clicking*, so the core culling loop was broken:
/// you could rate a photograph and then had to reach for the mouse to reach the next one. For a
/// tool whose whole purpose is going through thousands of frames, that is the difference between
/// usable and not.
///
/// The arithmetic is small and easy to get subtly wrong — an up-arrow on the first row, a
/// right-arrow on the last frame, a shift-extend that runs backwards — and every one of those
/// fails *silently* by moving somewhere plausible. So it lives here, where it can be tested.
///
/// # Columns
///
/// Left and right are index arithmetic. **Up and down are not** — they need to know how many tiles
/// are on a row, which depends on the window width and the tile size. The grid measures it and
/// passes it in; there is no way to derive it from the photograph list.
enum GridNavigation {
    /// Where a key press moves the cursor.
    enum Move {
        case left, right, up, down
        case first, last
        /// A whole screenful — the page keys, which is how a long shoot is traversed quickly.
        case pageUp, pageDown
    }

    /// The index a move lands on, or `nil` when it would leave the list.
    ///
    /// **`nil` rather than a clamp** for left, right, up and down: a cursor that sticks at the end
    /// is indistinguishable from one that is not moving, and the caller decides whether to stop or
    /// wrap. First and last always land somewhere.
    static func destination(
        from index: Int,
        move: Move,
        count: Int,
        columns: Int
    ) -> Int? {
        guard count > 0, index >= 0, index < count else { return nil }
        // **A column count of zero is a grid that has not been measured yet.** Treating it as one
        // column makes up and down behave like left and right, which is wrong but visible; the
        // alternative — dividing by it — is a crash on the first frame after a window resize.
        let cols = max(1, columns)

        switch move {
        case .left:
            return index > 0 ? index - 1 : nil
        case .right:
            return index < count - 1 ? index + 1 : nil
        case .up:
            return index - cols >= 0 ? index - cols : nil
        case .down:
            return index + cols < count ? index + cols : nil
        case .first:
            return 0
        case .last:
            return count - 1
        case .pageUp:
            // A page is a screenful, and landing on the *same* index is the same as not moving —
            // so a short list pages to the start rather than doing nothing.
            let page = cols * 4
            return max(0, index - page)
        case .pageDown:
            let page = cols * 4
            return min(count - 1, index + page)
        }
    }

    /// The selection after a move, given whether shift is held.
    ///
    /// # The two behaviours, and why they are different
    ///
    /// **Without shift**, the selection follows the cursor and is exactly one photograph — moving
    /// through a shoot replaces what is selected, which is what makes `⌫` act on what you are
    /// looking at.
    ///
    /// **With shift**, the selection is the **range between the anchor and the cursor**, so
    /// extending and then contracting works. Accumulating instead would mean a shift-arrow that
    /// overshoots can never be undone without starting again — and the anchor is what makes the
    /// range stable while it is adjusted.
    static func selection(
        after index: Int,
        photos: [Photo],
        anchor: Int64?,
        extending: Bool
    ) -> (ids: Set<Int64>, anchor: Int64?) {
        guard photos.indices.contains(index) else { return ([], anchor) }
        let id = photos[index].id

        guard extending, let anchor,
              let from = photos.firstIndex(where: { $0.id == anchor })
        else {
            // No anchor, or not extending: this move starts a new range.
            return ([id], id)
        }

        let range = from <= index ? from...index : index...from
        return (Set(photos[range].map(\.id)), anchor)
    }
}
