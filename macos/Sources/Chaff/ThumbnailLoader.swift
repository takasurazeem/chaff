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
            guard let path = try? EngineHolder.shared.thumbnailPath(for: id) else { return nil }
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

    func thumbnailPath(for id: Int64) throws -> String? {
        guard let engine else { return nil }
        return try engine.thumbnail(photoId: id, size: "grid")
    }
}
