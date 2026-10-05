import Foundation
import Observation
import chaff_ffiFFI

/// The application's state, and the only thing a view talks to.
///
/// # Why `@Observable` and not `ObservableObject`
///
/// `@Observable` tracks reads **per property**. A grid of 50,000 tiles that observes
/// `photos` re-renders when `photos` changes and not when `isIndexing` does — which matters
/// because a pass updates progress several times a second and a grid that re-rendered on every
/// tick would drop frames for no reason.
///
/// # Why the engine calls are `nonisolated`
///
/// `Engine` is a Rust object behind a lock. Calling it from the main actor would block the UI
/// for the length of a query — 21 ms for 50,000 photographs, which is more than a frame. So the
/// calls happen off the main actor and only the *results* come back to it.
///
/// The reviewer's finding is why a pass does **not** hold the read lock: it opens its own
/// connection, so the grid stays responsive while an index runs.
@MainActor
@Observable
final class EngineModel {
    /// Where the catalog and caches live.
    ///
    /// In the app's container, not beside the photographs: the library is the user's and this
    /// application does not write into it except through the trash.
    static func dataRoot() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL.temporaryDirectory
        let dir = base.appendingPathComponent("com.takasurazeem.chaff", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private(set) var photos: [Photo] = []
    private(set) var folders: [Folder] = []
    private(set) var library: Library?
    private(set) var tags: [(String, UInt32)] = []
    private(set) var people: [Person] = []
    private(set) var isIndexing = false
    /// `0...1`, or `nil` while the total is unknown.
    ///
    /// Optional rather than zero because the scan phase genuinely has no total — the size of a
    /// tree is not known until the walk finishes — and a determinate bar over an unknown total
    /// is a bar that lies.
    private(set) var progress: Double?
    private(set) var progressLabel = ""
    var errorMessage: String?

    private let engine: Engine
    private let database: URL

    init() {
        let root = Self.dataRoot()
        database = root.appendingPathComponent("catalog.db")
        setDataRoot(path: root.path)

        do {
            engine = try Engine(databasePath: database.path)
            // The thumbnail loader is an actor created by a static and cannot take a parameter,
            // so it is handed the engine once here rather than threaded through every view.
            EngineHolder.shared.set(engine)
        } catch {
            // A catalog that cannot be opened is not recoverable in-process — every later call
            // would fail the same way. Failing loudly at launch beats a UI where nothing works
            // and nothing says why.
            fatalError("could not open the catalog at \(database.path): \(error)")
        }
    }

    /// Open a library, indexing it if it has changed.
    func open(_ url: URL) async {
        isIndexing = true
        errorMessage = nil
        defer { isIndexing = false }

        let path = url.path(percentEncoded: false)
        do {
            let report = try await index(path)
            library = report.library
            photos = try engine.photos(libraryId: report.library.id)
            folders = try engine.folders(libraryId: report.library.id)
            tags = try engine.tags(libraryId: report.library.id).map { ($0.name, $0.count) }
            people = try engine.people(libraryId: report.library.id)
            progress = nil
        } catch {
            errorMessage = describe(error)
        }
    }

    /// Run the index off the main actor, with progress coming back to it.
    private func index(_ path: String) async throws -> OpenReport {
        let engine = self.engine
        return try await withCheckedThrowingContinuation { continuation in
            // A detached task, not `Task {}`: the pass must not inherit the main actor, or the
            // UI blocks for its whole duration.
            Task.detached(priority: .userInitiated) {
                do {
                    let report = try engine.openLibrary(
                        root: path,
                        progress: ProgressSink { done, total, stage, current in
                            // Hop back for the state write. The callback itself must not block —
                            // it runs on the pass's thread, and waiting on the main actor here
                            // would stall the pass.
                            Task { @MainActor in
                                self.progress = total > 0 ? Double(done) / Double(total) : nil
                                self.progressLabel = current.isEmpty
                                    ? "\(stage) \(done)"
                                    : "\(stage) \(done) · \(current)"
                            }
                        }
                    )
                    continuation.resume(returning: report)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    /// Everything known about one photograph.
    ///
    /// Off the main actor for the same reason as the list: a query is 21 ms on a large library
    /// and that is more than a frame. The panel is one call, not four — a panel that fetched
    /// EXIF, then files, then scores would arrive in three visible stages and the middle ones
    /// would look like bugs.
    func detail(for photoId: Int64) async throws -> PhotoDetail {
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.photoDetail(photoId: photoId)
        }.value
    }

    /// Write a decision.
    func setDecision(photoId: Int64, rating: UInt8, rejected: Bool) async throws {
        let engine = self.engine
        try await Task.detached(priority: .userInitiated) {
            try engine.setDecision(photoId: photoId, rating: rating, rejected: rejected)
        }.value
    }

    /// Reflect a decision in the loaded list, without a round trip.
    ///
    /// The write has already succeeded, so re-reading 50,000 records to learn what we just told
    /// the engine would be a second of work for a fact already in hand.
    func applyLocally(photoId: Int64, rating: UInt8, rejected: Bool) {
        guard let i = photos.firstIndex(where: { $0.id == photoId }) else { return }
        var p = photos[i]
        p.rating = rating
        p.rejected = rejected
        photos[i] = p
    }

    /// Restore a trashed operation.
    func restoreTrash(opId: String) async throws {
        guard let root = library?.root else {
            throw ChaffError.Engine(
                kind: .notFound,
                message: "no library is open, so there is nothing to restore into"
            )
        }
        let engine = self.engine
        _ = try await Task.detached(priority: .userInitiated) {
            try engine.restoreTrash(root: root, opId: opId)
        }.value
        // A restore changes what is on disk, so the list has to be re-read rather than patched.
        if let library {
            photos = try engine.photos(libraryId: library.id)
        }
    }

    /// Work out what a delete would move.
    func planDelete(photoIds: [Int64], root: String) async throws -> DeletePlan {
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.planDelete(root: root, photoIds: photoIds)
        }.value
    }

    /// Move what was shown.
    func commitDelete(root: String) async throws -> DeleteReceipt {
        let engine = self.engine
        let receipt = try await Task.detached(priority: .userInitiated) {
            try engine.commitDelete(root: root)
        }.value
        if let library {
            photos = try engine.photos(libraryId: library.id)
        }
        return receipt
    }

    func cancelDelete() {
        try? engine.cancelDelete()
    }

    /// A message a person can act on.
    ///
    /// The engine's `FailureKind` crosses the boundary as a Swift enum so this can branch —
    /// which it could not when the error type was `flat_error` and the kind was silently
    /// dropped.
    func describe(_ error: Error) -> String {
        guard case let ChaffError.Engine(kind, message) = error else {
            return error.localizedDescription
        }
        switch kind {
        case .poisoned:
            return "\(message) — restart Chaff."
        case .busy:
            return "\(message) — something else is using the catalog. Try again in a moment."
        case .notFound:
            return message
        case .refused:
            return message
        case .other:
            return message
        }
    }
}

/// Bridges the Rust callback interface to a Swift closure.
///
/// `Progress` is a UniFFI callback interface, so Swift has to supply a class. A struct with a
/// closure would be nicer and cannot conform to an `AnyObject` protocol.
private final class ProgressSink: Progress, @unchecked Sendable {
    private let handler: (UInt32, UInt32, String, String) -> Void

    init(_ handler: @escaping (UInt32, UInt32, String, String) -> Void) {
        self.handler = handler
    }

    func onProgress(done: UInt32, total: UInt32, stage: String, current: String) {
        handler(done, total, stage, current)
    }
}
