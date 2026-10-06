import AppKit
import chaff_ffiFFI

/// Loads thumbnails off the main actor, and remembers them.
///
/// # Why a cache on top of the engine's
///
/// The engine's cache is on **disk**, keyed by content hash. Reading it is a file open plus a
/// decode — fast, and not free. A grid that scrolls back over a row it has already shown should
/// not re-read from disk, and an `NSCache` is bounded and evicts under memory pressure for free.
///
/// # Why an actor
///
/// `NSImage` is not `Sendable`. An actor serialises access and hands back an image the main
/// actor can use, without `@unchecked Sendable` on a type that genuinely is not.
actor ThumbnailLoader {
    static let shared = ThumbnailLoader()

    private let memory = NSCache<NSNumber, NSImage>()
    /// In-flight loads, so a tile that scrolls out and back does not start a second read.
    private var inFlight: [Int64: Task<NSImage?, Never>] = [:]

    init() {
        // ~200 tiles at 180 pt, which is more than a screenful and small enough not to matter.
        memory.countLimit = 400
    }

    /// A larger render, for the loupe.
    func loupe(for id: Int64) async -> NSImage? {
        // One unwrap: `try?` **flattens** in modern Swift, so a throwing call returning `String?`
        // gives `String?` and not `String??`. The grid's loader learned this already.
        guard let path = try? EngineHolder.shared.thumbnailPath(for: id, size: "loupe") else {
            return nil
        }
        // Decoded off the main actor, like the grid's. A 1024-pixel decode is ~4 MB of pixels and
        // doing it in `body` is what turns paging through a shoot into a slideshow.
        return NSImage(contentsOfFile: path)
    }

    func image(for id: Int64) async -> NSImage? {
        if let hit = memory.object(forKey: NSNumber(value: id)) {
            return hit
        }
        if let existing = inFlight[id] {
            return await existing.value
        }

        let task = Task.detached(priority: .utility) { () -> NSImage? in
            // One unwrap, not two: `try?` **flattens** in modern Swift, so a throwing call
            // returning `String?` gives `String?` and not `String??`. I assumed the older
            // nesting and the compiler said otherwise.
            guard let path = try? EngineHolder.shared.gridPath(for: id) else { return nil }
            return NSImage(contentsOfFile: path)
        }
        inFlight[id] = task
        let image = await task.value
        inFlight[id] = nil
        if let image {
            memory.setObject(image, forKey: NSNumber(value: id))
        }
        return image
    }
}

/// The one `Engine`, for code that cannot be handed one.
///
/// A singleton because the loader is an actor created by a static and cannot take a parameter.
/// It is set once at launch from `EngineModel`, and reading it before that is a programming
/// error rather than a state to handle — the alternative is threading an engine through every
/// view for one call site.
final class EngineHolder: @unchecked Sendable {
    static let shared = EngineHolder()
    private var engine: Engine?

    func set(_ engine: Engine) { self.engine = engine }

    /// A thumbnail at a given size.
    ///
    /// Three sizes, and they are not interchangeable: **grid** is 256 and is what a scrolling
    /// wall of tiles needs; **loupe** is 1024 and is what fills a window; **zoom** is 2048 and is
    /// for judging focus at 100%. Asking for a loupe where a grid belongs decodes four times the
    /// pixels for every tile on screen, and asking for a grid in the loupe shows a photograph too
    /// soft to decide on.
    func thumbnailPath(for id: Int64, size: String = "grid") throws -> String? {
        guard let engine else { return nil }
        return try engine.thumbnail(photoId: id, size: size)
    }

    /// The old single-size entry point, kept because the grid calls it on every tile.
    func gridPath(for id: Int64) throws -> String? {
        guard let engine else { return nil }
        return try engine.thumbnail(photoId: id, size: "grid")
    }

}
