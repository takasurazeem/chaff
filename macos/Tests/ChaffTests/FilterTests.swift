import Testing
import chaff_ffiFFI
@testable import Chaff

/// The filter, which produces an **empty grid with no error and no clue** when it is wrong.
///
/// The folder comparison beside it was silently broken for a while — a relative path against an
/// absolute one, showing nothing for a folder labelled 2,071 photographs. These are the tests that
/// would have caught it, applied to the thing that replaced it.
struct FilterTests {
    /// A photograph with just the fields the filter reads.
    private func photo(
        id: Int64,
        band: String? = nil,
        rating: UInt8 = 0,
        rejected: Bool = false,
        camera: String? = nil,
        lens: String? = nil,
        year: Int32? = nil
    ) -> Photo {
        Photo(
            id: id, stem: "IMG_\(id)", dir: "/lib", state: "pair", needsReview: false,
            composite: nil, band: band, rating: rating, rejected: rejected,
            camera: camera, lens: lens, year: year
        )
    }

    private var library: [Photo] {
        [
            photo(id: 1, band: "keep", rating: 5, camera: "Canon EOS RP", lens: "RF 24-105", year: 2026),
            photo(id: 2, band: "review", rating: 0, camera: "Canon EOS RP", lens: "RF 24-105", year: 2026),
            photo(id: 3, band: "reject", rejected: true, camera: "Canon EOS RP", lens: "RF 50", year: 2025),
            photo(id: 4, band: nil, camera: nil, lens: nil, year: nil),
        ]
    }

    @Test("An empty filter keeps everything")
    func emptyKeepsEverything() {
        let f = Filters()
        #expect(!f.isActive)
        #expect(library.allSatisfy(f.matches))
    }

    @Test("A band filter keeps only that band")
    func bandFilters() {
        var f = Filters()
        f.band = .keep
        #expect(library.filter(f.matches).map(\.id) == [1])

        f.band = .reject
        #expect(library.filter(f.matches).map(\.id) == [3])
    }

    @Test("An unscored photograph is in All and in no band")
    func unscoredBelongsToNoBand() {
        // **The direction that matters.** A photograph with no band produced no measurement — a
        // raw this build cannot decode. A filter that fell through to `true` for it would show it
        // under "Keep", which is a claim nobody made.
        var f = Filters()
        f.band = .keep
        #expect(!f.matches(library[3]), "an unscored photograph is not a keep")
        f.band = .review
        #expect(!f.matches(library[3]))
        f.band = .reject
        #expect(!f.matches(library[3]))
        f.band = .all
        #expect(f.matches(library[3]), "but it is still a photograph")
    }

    @Test("Rated and unrated are a partition of what is not rejected")
    func decisionPartitions() {
        // **The counts have to add up**, or the chips contradict each other. A rejected
        // photograph keeps whatever stars it had, so it is neither rated nor unrated in the sense
        // a user means — it is rejected.
        let counts = Filters.decisionCounts(library)
        #expect(counts[.all] == 4)
        #expect(counts[.rated] == 1, "one has stars")
        #expect(counts[.rejected] == 1)
        #expect(counts[.unrated] == 2, "two are neither rated nor rejected")
        #expect(
            counts[.rated]! + counts[.unrated]! + counts[.rejected]! == counts[.all]!,
            "the three must partition the library: \(counts)"
        )
    }

    @Test("A rejected photograph is not counted as rated even when it has stars")
    func rejectedIsNotRated() {
        // Rejecting does **not** clear the rating — that is deliberate, so un-rejecting restores
        // what the user thought. But it means the two counts overlap unless rejected is excluded.
        let photos = [photo(id: 1, rating: 3, rejected: true)]
        let counts = Filters.decisionCounts(photos)
        #expect(counts[.rejected] == 1)
        #expect(counts[.rated] == 1, "it does have stars")
        #expect(counts[.unrated] == 0)
    }

    @Test("A facet filter matches exactly, not by prefix")
    func facetsAreExact() {
        // **A prefix match would be wrong and look right.** "RF 24-105" and "RF 24-105 F4L" are
        // different lenses, and a filter for the first showing photographs taken with the second
        // is a filter that lies.
        var f = Filters()
        f.lens = "RF 24-105"
        #expect(library.filter(f.matches).map(\.id) == [1, 2])
        #expect(!f.matches(library[2]), "the RF 50 is a different lens")
    }

    @Test("A missing camera or year does not match a filter for one")
    func absentDoesNotMatch() {
        // `nil` on the photograph against a set filter. The `?? false` of this comparison is one
        // character and it is the whole difference — a stripped JPEG is not a Canon.
        var f = Filters()
        f.camera = "Canon EOS RP"
        #expect(!f.matches(library[3]), "no camera recorded is not this camera")

        f = Filters()
        f.year = 2026
        #expect(!f.matches(library[3]), "no year recorded is not this year")
    }

    @Test("Filters compose rather than replace each other")
    func filtersCompose() {
        // A user who selects a camera and then a band means **both**. A filter that replaced one
        // with the other would show photographs they had just excluded.
        var f = Filters()
        f.camera = "Canon EOS RP"
        f.band = .keep
        #expect(library.filter(f.matches).map(\.id) == [1])

        f.lens = "RF 50"
        #expect(library.filter(f.matches).isEmpty, "no keep was shot on the RF 50")
    }

    @Test("The band counts are of the library, not of the current filter")
    func countsDoNotShift() {
        // "Reject 312" that became "Reject 4" once Keep was selected would be describing the
        // filter rather than the library, and the number a user is looking for is about the
        // library.
        let counts = Filters.bandCounts(library)
        #expect(counts[.all] == 4)
        #expect(counts[.keep] == 1)
        #expect(counts[.review] == 1)
        #expect(counts[.reject] == 1)

        // And they are the same whatever is selected, because they are computed from `photos`.
        var f = Filters()
        f.band = .keep
        #expect(Filters.bandCounts(library)[.reject] == 1)
    }

    @Test("The facets come from the library and omit what is absent")
    func facetsOmitAbsent() {
        // A "Camera" menu with an empty entry in it is a bug the user cannot explain. And a year
        // of `0` is not a year.
        let facets = Filters.facets(library)
        #expect(facets.cameras == ["Canon EOS RP"], "one camera, and no empty string")
        #expect(facets.lenses == ["RF 24-105", "RF 50"])
        #expect(facets.years == [2026, 2025], "newest first")
        #expect(!facets.cameras.contains(""), "an absent camera must not become an entry")
    }

    @Test("isActive is true for every way of narrowing")
    func isActiveCoversEveryField() {
        // Drives the Clear button. A filter that is applied and does not offer a way back is one
        // a user has to guess their way out of.
        #expect(!Filters().isActive)

        var f = Filters()
        f.band = .keep
        #expect(f.isActive, "a band did not mark the filter active")

        f = Filters()
        f.decision = .rated
        #expect(f.isActive, "a decision did not mark the filter active")

        f.camera = "x"
        #expect(f.isActive)
        f = Filters()
        f.lens = "x"
        #expect(f.isActive)
        f = Filters()
        f.year = 2026
        #expect(f.isActive)
    }
}
