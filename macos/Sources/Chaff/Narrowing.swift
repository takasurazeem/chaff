import Foundation

/// What the grid is narrowed to, when it is not simply "the whole library".
///
/// # Why this is one value and not two optionals
///
/// A folder, a tag and a group are three ways to answer the same question — *which photographs
/// am I looking at?* — and the interface can only show one answer at a time. Two independent
/// optionals would let a caller set both, and the grid would then have to decide which wins,
/// which is a decision that belongs in one place rather than in a filter expression.
///
/// The status line reads this too: "412 of 2,956 photographs" says *how many*, and this says
/// **why**.
enum Narrowing: Hashable, Identifiable {
    case tag(String)
    case person(id: Int64, name: String?)

    var id: String {
        switch self {
        case let .tag(name): "tag:\(name)"
        case let .person(id, _): "person:\(id)"
        }
    }

    /// Does a photograph belong to what the grid is narrowed to?
    ///
    /// # Why this lives here and not on the view
    ///
    /// It was a private method on `LibraryView`, which made it **untestable** — and it is exactly
    /// the kind of logic that fails silently: a tag that does not match produces an empty grid
    /// with no error and no clue. The folder comparison beside it *was* silently broken for a
    /// while, matching a relative path against an absolute one and showing nothing.
    ///
    /// The lookups are passed in rather than reached for, so a test can build them without an
    /// engine, a catalog or a pass.
    func matches(
        photoId: Int64,
        tagsByPhoto: [Int64: Set<String>],
        peopleByPhoto: [Int64: Set<Int64>]
    ) -> Bool {
        switch self {
        case let .tag(name): tagsByPhoto[photoId]?.contains(name) ?? false
        case let .person(id, _): peopleByPhoto[photoId]?.contains(id) ?? false
        }
    }

    /// What to call it in the status line.
    var label: String {
        switch self {
        case let .tag(name): "tagged “\(name)”"
        case let .person(_, name): name.map { "in \($0)" } ?? "in one unnamed group"
        }
    }
}
