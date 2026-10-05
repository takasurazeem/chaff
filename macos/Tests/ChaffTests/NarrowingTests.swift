import Testing
@testable import Chaff

/// What the grid is narrowed to, and whether a photograph belongs to it.
///
/// **This logic fails silently.** A tag that does not match produces an empty grid with no error
/// and no clue — and the folder comparison beside it *was* silently broken, matching a relative
/// path against an absolute one and showing nothing at all. So these are not tests of a filter;
/// they are tests of the thing that made a 2,071-photograph folder look empty.
struct NarrowingTests {
    private let tags: [Int64: Set<String>] = [
        1: ["sunset", "travel"],
        2: ["travel"],
        3: [],
    ]
    private let people: [Int64: Set<Int64>] = [
        1: [10],
        2: [10, 11],
        3: [],
    ]

    @Test("A tag matches the photographs that carry it, and only those")
    func tagMatches() {
        let n = Narrowing.tag("travel")
        #expect(n.matches(photoId: 1 as Int64, tagsByPhoto: tags, peopleByPhoto: people))
        #expect(n.matches(photoId: 2 as Int64, tagsByPhoto: tags, peopleByPhoto: people))
        #expect(!n.matches(photoId: 3 as Int64, tagsByPhoto: tags, peopleByPhoto: people))
    }

    @Test("A tag nobody carries matches nothing, rather than everything")
    func unknownTagMatchesNothing() {
        // **The direction that matters.** A filter that fell through to `true` for an unknown tag
        // would show the whole library while the status line claimed a filter — which is worse
        // than showing nothing, because nothing is at least obviously wrong.
        let n = Narrowing.tag("a tag no photograph has")
        for id: Int64 in [1, 2, 3] {
            #expect(!n.matches(photoId: id, tagsByPhoto: tags, peopleByPhoto: people))
        }
    }

    @Test("A photograph with no tags at all matches nothing")
    func aPhotographWithNoTags() {
        // The `?? false` in the lookup. An empty set and an absent key must behave the same —
        // a photograph nobody has tagged is not in any tag's results.
        #expect(!Narrowing.tag("travel").matches(photoId: 3 as Int64, tagsByPhoto: tags, peopleByPhoto: people))
        #expect(!Narrowing.tag("travel").matches(photoId: 999 as Int64, tagsByPhoto: tags, peopleByPhoto: people))
    }

    @Test("A person matches the photographs in their group")
    func personMatches() {
        let n = Narrowing.person(id: 10, name: "Alice")
        #expect(n.matches(photoId: 1 as Int64, tagsByPhoto: tags, peopleByPhoto: people))
        #expect(n.matches(photoId: 2 as Int64, tagsByPhoto: tags, peopleByPhoto: people))
        #expect(!n.matches(photoId: 3 as Int64, tagsByPhoto: tags, peopleByPhoto: people))
    }

    @Test("An unnamed group matches the same as a named one")
    func nameDoesNotAffectMatching() {
        // **The name is for the status line, not for the lookup.** Matching by name would break
        // the moment a group was renamed — and renaming is the action this whole feature exists
        // for.
        let named = Narrowing.person(id: 10, name: "Alice")
        let unnamed = Narrowing.person(id: 10, name: nil)
        for id: Int64 in [1, 2, 3] {
            #expect(
                named.matches(photoId: id, tagsByPhoto: tags, peopleByPhoto: people)
                    == unnamed.matches(photoId: id, tagsByPhoto: tags, peopleByPhoto: people),
                "photo \(id) matched differently by name"
            )
        }
    }

    @Test("Two different narrowings are not equal, so the grid re-filters")
    func identityIsDistinct() {
        // The `id` drives `.task(id:)` and list selection. Two narrowings sharing an id would
        // mean selecting a second tag did not re-filter, and the grid would show the first one's
        // results under the second one's heading.
        #expect(Narrowing.tag("a").id != Narrowing.tag("b").id)
        #expect(Narrowing.person(id: 1, name: "x").id != Narrowing.person(id: 2, name: "x").id)
        #expect(Narrowing.tag("a").id != Narrowing.person(id: 1, name: "a").id)
    }

    @Test("The status line label names what the filter is, not just that there is one")
    func labelSaysWhat() {
        // "412 of 2,956 photographs" says how many. A filter that hides 2,900 without saying what
        // it filtered on is a filter that looks like a bug.
        #expect(Narrowing.tag("sunset").label.contains("sunset"))
        #expect(Narrowing.person(id: 1, name: "Alice").label.contains("Alice"))
        // An unnamed group still has to say *something* — the alternative is a label that reads
        // "in " and trails off.
        #expect(!Narrowing.person(id: 1, name: nil).label.isEmpty)
        #expect(Narrowing.person(id: 1, name: nil).label.contains("unnamed"))
    }
}
