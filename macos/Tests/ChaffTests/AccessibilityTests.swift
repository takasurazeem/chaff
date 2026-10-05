import Testing
@testable import Chaff

/// What VoiceOver reads for a tile.
///
/// A culling tool exists to help someone decide which photographs to keep, and the decision rests
/// on the score and on what has already been decided. A grid that reads filenames to a screen
/// reader is a grid that cannot be culled without sight — so these are not cosmetic assertions.
struct AccessibilityTests {
    @Test("A scored, rated, rejected photograph says all three")
    func everythingIsAnnounced() {
        let value = accessibilityValue(score: 32, rating: 4, rejected: true, isSelected: false)
        #expect(value.contains("32"))
        #expect(value.contains("4 stars"))
        #expect(value.contains("rejected"))
    }

    @Test("An unrated photograph does not say zero stars")
    func zeroIsUnratedNotZeroStars() {
        // **The distinction that matters most in a large library.** Most photographs are
        // unrated, and reading "0 stars" for each of 3,000 frames is noise in exactly the place
        // a user is listening for signal.
        let value = accessibilityValue(score: 55, rating: 0, rejected: false, isSelected: false)
        #expect(!value.contains("star"))
        #expect(value.contains("55"))
    }

    @Test("One star is singular")
    func oneStarIsSingular() {
        #expect(accessibilityValue(score: nil, rating: 1, rejected: false, isSelected: false)
            .contains("1 star"))
        #expect(accessibilityValue(score: nil, rating: 2, rejected: false, isSelected: false)
            .contains("2 stars"))
    }

    @Test("A photograph with no score says so rather than staying silent")
    func unscoredIsAnnounced() {
        // **Said, not skipped.** A photograph with no score is one that produced no measurement
        // — a raw this build cannot decode. Silence there reads as "fine", which is the one
        // thing it is not.
        let value = accessibilityValue(score: nil, rating: 0, rejected: false, isSelected: false)
        #expect(value.contains("not scored"))
    }

    @Test("Selection is announced last, so the numbers come first")
    func selectionComesLast() {
        // The order is the reading order. "selected, 32, 4 stars" buries the decision under the
        // state; the state belongs at the end.
        let value = accessibilityValue(score: 32, rating: 4, rejected: false, isSelected: true)
        let parts = value.components(separatedBy: ", ")
        #expect(parts.last == "selected")
        #expect(parts.first?.contains("32") == true)
    }

    @Test("Rejected is announced even when the rating is zero")
    func rejectedSurvivesAnUnratedFrame() {
        // **Rejecting does not clear the rating**, so a rejected frame can legitimately have no
        // stars — and the one fact a user must hear about it is that it is rejected.
        let value = accessibilityValue(score: nil, rating: 0, rejected: true, isSelected: false)
        #expect(value.contains("rejected"))
        #expect(value.contains("not scored"))
    }

    @Test("Every navigator mode has a spoken name")
    func navigatorModesAreNamed() {
        // An icon-only segmented control with no `accessibilityLabel` announces as "button", so
        // a screen reader user hears three identical buttons and cannot tell folders from people.
        // `.help()` is a tooltip and is not read as the control's name.
        for mode in Navigator.Mode.allCases {
            #expect(!mode.help.isEmpty, "\(mode) has no spoken name")
            #expect(!mode.symbol.isEmpty, "\(mode) has no symbol")
        }
    }
}
