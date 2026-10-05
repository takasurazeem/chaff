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

    /// What to call it in the status line.
    var label: String {
        switch self {
        case let .tag(name): "tagged “\(name)”"
        case let .person(_, name): name.map { "in \($0)" } ?? "in one unnamed group"
        }
    }
}
