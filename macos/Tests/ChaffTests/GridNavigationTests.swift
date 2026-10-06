import Testing
import chaff_ffiFFI
@testable import Chaff

/// Moving the cursor around the grid.
///
/// # Why these are worth having
///
/// **The grid had no keyboard navigation at all.** The menu rates with `1`–`5` and they act on the
/// cursor — but the cursor could only be set by clicking, so you could rate a photograph and then
/// had to reach for the mouse to reach the next one. For a tool whose whole purpose is going
/// through thousands of frames, that is the difference between usable and not.
///
/// The arithmetic is small and every error in it fails **silently** by moving somewhere plausible:
/// an up-arrow on the first row, a right-arrow on the last frame, a shift-extend that runs
/// backwards.
struct GridNavigationTests {
    private func photos(_ n: Int) -> [Photo] {
        (1...n).map { i in
            Photo(
                id: Int64(i), stem: "IMG_\(i)", dir: "/lib", state: "pair", needsReview: false,
                composite: nil, band: nil, rating: 0, rejected: false,
                focus: nil, noise: nil, detail: nil, camera: nil, lens: nil, year: nil
            )
        }
    }

    /// A 10-photograph list in rows of 3.
    private func destination(_ from: Int, _ move: GridNavigation.Move) -> Int? {
        GridNavigation.destination(from: from, move: move, count: 10, columns: 3)
    }

    @Test("Left and right move by one")
    func horizontal() {
        #expect(destination(4, .left) == 3)
        #expect(destination(4, .right) == 5)
    }

    @Test("Up and down move by a row, not by one")
    func vertical() {
        // **The whole reason columns are measured.** Moving by one on a down-arrow would walk
        // along the row the user is already looking at, which reads as the key doing nothing.
        #expect(destination(4, .down) == 7)
        #expect(destination(7, .up) == 4)
    }

    @Test("Up on the first row stays put rather than wrapping")
    func upAtTheTop() {
        // **`nil`, not a wrap.** Wrapping to the bottom of a 3,000-photograph library on an
        // accidental up-arrow is a jump nobody can explain.
        #expect(destination(0, .up) == nil)
        #expect(destination(2, .up) == nil)
        #expect(destination(3, .up) == 0, "the second row does move up")
    }

    @Test("Down on the last row stays put")
    func downAtTheBottom() {
        // The last row is short: indices 9 is the only one, and 10+ does not exist.
        #expect(destination(9, .down) == nil)
        #expect(destination(8, .down) == nil, "8 + 3 is past the end")
    }

    @Test("Left and right stop at the ends rather than wrapping")
    func horizontalEnds() {
        #expect(destination(0, .left) == nil)
        #expect(destination(9, .right) == nil)
    }

    @Test("Home and End always land somewhere")
    func homeAndEnd() {
        // The two moves that cannot fail — an arrow at the edge does nothing, but Home in an
        // empty library should still not crash.
        #expect(destination(5, .first) == 0)
        #expect(destination(5, .last) == 9)
        #expect(GridNavigation.destination(from: 0, move: .first, count: 0, columns: 3) == nil)
        #expect(GridNavigation.destination(from: 0, move: .last, count: 0, columns: 3) == nil)
    }

    @Test("Paging moves a screenful and lands inside the list")
    func paging() {
        // A page is four rows. `min`/`max` rather than `nil`, because a page key that does
        // nothing at the end is indistinguishable from a broken key.
        #expect(destination(0, .pageDown) == 10 - 1, "four rows of three is past the end")
        #expect(destination(9, .pageUp) == 0)
        #expect(destination(4, .pageDown) == 9)
    }

    @Test("A grid with no measured columns does not divide by zero")
    func unmeasuredColumns() {
        // **The state on the first frame**, before the geometry reader has run. Treating it as one
        // column makes up and down behave like left and right — wrong but visible. Dividing by it
        // is a crash on every window resize.
        let d = GridNavigation.destination(from: 4, move: .down, count: 10, columns: 0)
        #expect(d == 5, "one column means down is the next photograph")
    }

    @Test("A move without shift replaces the selection and starts a new range")
    func plainMoveReplaces() {
        // Moving through a shoot replaces what is selected, which is what makes `⌫` act on what
        // you are looking at.
        let list = photos(5)
        let (ids, anchor) = GridNavigation.selection(
            after: 2, photos: list, anchor: 1, extending: false
        )
        #expect(ids == [3])
        #expect(anchor == 3, "the anchor follows, so a later shift-extend starts here")
    }

    @Test("A shift-extend selects the range between the anchor and the cursor")
    func extendSelectsTheRange() {
        let list = photos(5)
        // **`anchor` is an id, not an index** — `anchor: 2` means the photograph with id 2, which
        // sits at index 1. So the range runs index 1...3, which is ids 2...4.
        let (ids, anchor) = GridNavigation.selection(
            after: 3, photos: list, anchor: 2, extending: true
        )
        #expect(ids == [2, 3, 4], "inclusive of both ends")
        #expect(anchor == 2, "the anchor does not move while extending")
    }

    @Test("Extending backwards selects the same range")
    func extendBackwards() {
        // **The case a naive `from...index` crashes on**: a range whose lower bound is above its
        // upper is a runtime trap, not an empty selection.
        let list = photos(5)
        // Anchor id 4 is at index 3; the cursor is index 1. The range runs 1...3 → ids 2...4.
        let (ids, _) = GridNavigation.selection(
            after: 1, photos: list, anchor: 4, extending: true
        )
        #expect(ids == [2, 3, 4])
    }

    @Test("Extending and then contracting shrinks the range rather than accumulating")
    func contract() {
        // **Why the anchor exists.** If the selection accumulated, a shift-arrow that overshot
        // could never be undone without starting again.
        let list = photos(5)
        let (wide, anchor) = GridNavigation.selection(
            after: 4, photos: list, anchor: 1, extending: true
        )
        #expect(wide == [1, 2, 3, 4, 5], "anchor id 1 is index 0; the cursor is index 4")

        let (narrow, _) = GridNavigation.selection(
            after: 2, photos: list, anchor: anchor, extending: true
        )
        #expect(narrow == [1, 2, 3], "contracting back removes the far end")
    }

    @Test("A shift-extend with no anchor behaves like a plain move")
    func extendWithoutAnchor() {
        // The state after a filter change clears the anchor. Extending from nowhere is not an
        // error — it is the first click of a new range.
        let list = photos(5)
        let (ids, anchor) = GridNavigation.selection(
            after: 2, photos: list, anchor: nil, extending: true
        )
        #expect(ids == [3])
        #expect(anchor == 3)
    }

    @Test("An index outside the list selects nothing rather than crashing")
    func outOfRange() {
        // A filter can shrink the list while the cursor still points at a photograph that is no
        // longer in it. Subscripting there is a crash, and the user would have no idea why.
        let list = photos(3)
        let (ids, _) = GridNavigation.selection(after: 9, photos: list, anchor: nil, extending: false)
        #expect(ids.isEmpty)
    }
}
